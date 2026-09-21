//! Default routine seeding [ORB-10129].
//!
//! Routines are workspace-authored YAML under `.orbit/routines/` — unlike
//! activities and jobs there is no global routines directory, so defaults
//! are seeded per workspace on `orbit init`. Three placeholders are resolved
//! at seed time:
//!
//! - `__ORBIT_ROUTINE_NAME__` — routine names must be unique across all
//!   routine sources on a host, so the seeded name carries the registered
//!   workspace name as a suffix (`task-pilot-<workspace-name>`) to keep two
//!   seeded source workspaces from colliding fail-closed. The suffix comes
//!   from the workspace name the operator registered, never from the checkout
//!   directory: two checkouts whose directories share a basename would
//!   otherwise seed the same names on one host [ORB-12107].
//! - `__ORBIT_OWNER_MACHINE__` and `__ORBIT_BASE_BRANCH__` — a state trigger
//!   names the one machine that evaluates it and the branch it observes, so
//!   the seeded `task_pilot` definition renders this host's registered
//!   machine id and the workspace's registered base branch [ORB-12745].
//!
//! Cron definitions carry no host pin [ORB-12236]: two hosts initializing the
//! same workspace name write byte-identical cron definitions, and differ only
//! in the state trigger's owner. Seeded routines are disabled when written;
//! they exist so a fresh workspace gets reviewable, opt-in schedules without
//! silently enabling unattended work.
//!
//! Provenance is byte-exact first and shape-aware second. The manifest records
//! the digest Orbit last wrote; a file that still matches is Orbit's. A file
//! that differs only in what the operator owns — the `enabled` opt-in (the
//! documented lifecycle, and what the dashboard toggle edits), the retired
//! `hosts:` key the loader tells operators to drop, and comments — is still
//! Orbit's when its template-owned fields match a template this or a prior
//! release shipped for that stem ([`SUPERSEDED_ROUTINE_TEMPLATES`],
//! [`RETIRED_ROUTINE_FILES`]). Anything else is a local edit and is preserved.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use orbit_automation::routines::loader::{declared_routine_names, retired_routine_job_reason};
use orbit_common::OrbitError;
use orbit_common::protocol::yaml::parse_routine_yaml;
use orbit_types::workflow::RoutineDefinition;

use super::routines::rewrite_enabled_line;
use super::{
    MANAGED_ASSET_MANIFEST_FILE, ManagedAssetAction, ManagedAssetLayout, ManagedAssetManifest,
    ManagedAssetOutcome, ManagedAssetReconcileMode, ManagedAssetReconciliation,
    ROUTINE_MANAGED_ASSET_MANIFEST_SCHEMA_VERSION, RoutineAssetProvenance,
    RoutineMaterializationBinding, encode_managed_asset_manifest, load_managed_asset_manifest,
    preserve_modified_retired_asset, retired_preservation_path, sha256_hex,
};
use orbit_common::fs::io::{atomic_write_text, write_text_with_parent};

/// Shippable default routine assets, seeded under
/// `<workspace>/.orbit/routines/<file>.yaml` on `orbit init`. Every entry
/// must keep the `__ORBIT_ROUTINE_NAME__` placeholder parseable once
/// substituted — `seed_default_routines` validates each rendered document
/// fail-closed before writing.
pub(crate) const DEFAULT_ROUTINE_FILES: &[(&str, &str)] = &[
    (
        "ci_failure_sweep",
        include_str!("../../assets/routines/ci_failure_sweep.yaml"),
    ),
    (
        "dependabot_alert_sweep",
        include_str!("../../assets/routines/dependabot_alert_sweep.yaml"),
    ),
    (
        "task_pilot",
        include_str!("../../assets/routines/task_pilot.yaml"),
    ),
    (
        "ship_sweep",
        include_str!("../../assets/routines/ship_sweep.yaml"),
    ),
    (
        "worktree_gc",
        include_str!("../../assets/routines/worktree_gc.yaml"),
    ),
];

/// Default routines a prior release seeded that this Orbit no longer ships,
/// each as the last template it shipped. `orbit workspace sync` retires an
/// on-disk copy whose template-owned fields still match one of these; the
/// loader skips a routine targeting its job as retired instead of failing it
/// on every clock tick (`RETIRED_ROUTINE_JOBS`).
///
/// Retiring a default: move its template here and add its target job to
/// `RETIRED_ROUTINE_JOBS` in the same change.
pub(crate) const RETIRED_ROUTINE_FILES: &[(&str, &str)] = &[
    (
        "auto_task_scheduler",
        include_str!("../../assets/routines/retired/auto_task_scheduler.yaml"),
    ),
    (
        "task_triage",
        include_str!("../../assets/routines/retired/task_triage.yaml"),
    ),
];

/// Earlier shapes of routines this Orbit still ships: every template whose
/// template-owned fields differed from the current one. A workspace seeded by
/// that release and since opted in (`enabled: true`) is refreshed onto the
/// current template with its opt-in kept, instead of being reported as
/// locally modified forever.
///
/// Changing a shipped template's fields (not its comments): copy the previous
/// version here as `<stem>.<last-shipped-date>.yaml` in the same change.
pub(crate) const SUPERSEDED_ROUTINE_TEMPLATES: &[(&str, &str)] = &[
    (
        "ci_failure_sweep",
        include_str!("../../assets/routines/superseded/ci_failure_sweep.2026-08-30.yaml"),
    ),
    (
        "task_pilot",
        include_str!("../../assets/routines/superseded/task_pilot.2026-08-15.yaml"),
    ),
    (
        "task_pilot",
        include_str!("../../assets/routines/superseded/task_pilot.2026-09-21.yaml"),
    ),
    (
        "worktree_gc",
        include_str!("../../assets/routines/superseded/worktree_gc.2026-07-12.yaml"),
    ),
];

// Widened to pub(crate) for test access in sibling tests/routine.rs.
pub(crate) const ROUTINE_NAME_PLACEHOLDER: &str = "__ORBIT_ROUTINE_NAME__";
/// The registered machine id a seeded state trigger names as its owner.
pub(crate) const OWNER_MACHINE_PLACEHOLDER: &str = "__ORBIT_OWNER_MACHINE__";
/// The registered base branch a seeded state trigger observes; the same
/// placeholder the delivery auto-task defaults use.
pub(crate) const BASE_BRANCH_PLACEHOLDER: &str = super::auto_tasks::BASE_BRANCH_PLACEHOLDER;

/// The identity a workspace's default routines are materialized against: the
/// registered workspace name their names are suffixed with, plus the host
/// machine id and registered base branch a state trigger is bound to.
///
/// Construction validates the name, which is why the fields are private: an
/// existing value always renders a loadable routine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineSeedIdentity {
    name_suffix: String,
    owner_machine: String,
    base_branch: String,
}

impl RoutineSeedIdentity {
    /// Build the seed identity for `workspace_name` on the host registered
    /// as `machine_id`, observing `base_branch`. Rejects a workspace name
    /// with no characters usable in a routine name — that name would
    /// otherwise silently fall back to a host-wide unsuffixed routine — and
    /// a blank machine id or branch, which no state trigger validates with.
    pub fn new(
        workspace_name: &str,
        machine_id: &str,
        base_branch: &str,
    ) -> Result<Self, OrbitError> {
        let name_suffix = sanitize_routine_name_part(workspace_name);
        if name_suffix.is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "workspace name '{workspace_name}' has no characters usable in a routine name; \
                 routine names must stay unique across every routine source on this host, so \
                 choose a workspace name containing letters or digits"
            )));
        }
        if machine_id.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "seeding default routines requires this host's registered machine id; run `orbit init` first".to_string(),
            ));
        }
        if base_branch.is_empty() || base_branch.chars().any(char::is_whitespace) {
            return Err(OrbitError::InvalidInput(format!(
                "workspace base branch '{base_branch}' cannot be observed by a seeded routine"
            )));
        }

        Ok(Self {
            name_suffix,
            owner_machine: machine_id.trim().to_string(),
            base_branch: base_branch.to_string(),
        })
    }

    /// The binding `template` renders against on this host: the workspace
    /// routine name, plus the owner and branch only when the template asks
    /// for them, so a cron default's provenance stays host-independent.
    fn binding(&self, file_stem: &str, template: &str) -> RoutineMaterializationBinding {
        RoutineMaterializationBinding {
            name: self.routine_name(file_stem),
            owner_machine: template
                .contains(OWNER_MACHINE_PLACEHOLDER)
                .then(|| self.owner_machine.clone()),
            branch: template
                .contains(BASE_BRANCH_PLACEHOLDER)
                .then(|| self.base_branch.clone()),
        }
    }

    /// A recorded binding completed for `template`: a routine adopted or
    /// recorded before its template gained a state trigger has a name but no
    /// owner or branch, and takes this host's. A binding that already
    /// carries them keeps them — a recorded owner is preserved exactly as a
    /// recorded name is.
    fn complete(
        &self,
        file_stem: &str,
        template: &str,
        binding: &RoutineMaterializationBinding,
    ) -> RoutineMaterializationBinding {
        let requested = self.binding(file_stem, template);
        RoutineMaterializationBinding {
            name: binding.name.clone(),
            owner_machine: binding.owner_machine.clone().or(requested.owner_machine),
            branch: binding.branch.clone().or(requested.branch),
        }
    }

    /// Compose a per-workspace routine name: `<stem>-<workspace-name>`, using
    /// the routine name charset (lowercase alphanumeric plus `-`/`_`, starting
    /// alphanumeric). Names must be unique across all routine sources on a
    /// host, so the workspace suffix is what lets two seeded sources coexist.
    pub(crate) fn routine_name(&self, file_stem: &str) -> String {
        format!("{}-{}", file_stem.replace('_', "-"), self.name_suffix)
    }

    /// Every name this identity would seed, in [`DEFAULT_ROUTINE_FILES`] order.
    pub fn seeded_routine_names(&self) -> Vec<String> {
        DEFAULT_ROUTINE_FILES
            .iter()
            .map(|(stem, _)| self.routine_name(stem))
            .collect()
    }
}

/// A name this workspace's default routines would take that another workspace
/// on the same host already declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineNameCollision {
    /// The routine name claimed twice.
    pub name: String,
    /// The already-existing definition file that claims it.
    pub declared_in: PathBuf,
}

/// Names that seeding for `identity` would take from routine definitions
/// already declared under `other_orbit_dirs` (every registered checkout on
/// this host except the one being initialized).
///
/// A name defined by more than one source is a load-time error that drops
/// *every* colliding definition, so init reports this instead of writing a
/// set of routines that can never fire [ORB-12107].
pub fn default_routine_name_collisions(
    identity: &RoutineSeedIdentity,
    other_orbit_dirs: &[PathBuf],
) -> Vec<RoutineNameCollision> {
    let seeded: BTreeSet<String> = identity.seeded_routine_names().into_iter().collect();
    let mut collisions: BTreeMap<String, PathBuf> = BTreeMap::new();

    for orbit_dir in other_orbit_dirs {
        for (name, path) in declared_routine_names(orbit_dir) {
            if seeded.contains(&name) {
                collisions.entry(name).or_insert(path);
            }
        }
    }

    collisions
        .into_iter()
        .map(|(name, declared_in)| RoutineNameCollision { name, declared_in })
        .collect()
}

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

pub(crate) fn reconcile_default_routines(
    routines_dir: &Path,
    identity: &RoutineSeedIdentity,
    overwrite_bindings: bool,
    mode: ManagedAssetReconcileMode,
) -> Result<ManagedAssetReconciliation, OrbitError> {
    let manifest_path = routines_dir.join(MANAGED_ASSET_MANIFEST_FILE);
    let previous =
        load_managed_asset_manifest(&manifest_path, "routine", ManagedAssetLayout::YamlStem)?;
    let shipped: BTreeSet<&str> = DEFAULT_ROUTINE_FILES
        .iter()
        .map(|(name, _)| *name)
        .collect();
    let mut next_assets = BTreeMap::new();
    let mut next_provenance = BTreeMap::new();
    let mut result = ManagedAssetReconciliation::default();

    if let Some(previous) = &previous {
        for (name, rendered_digest) in &previous.assets {
            if shipped.contains(name.as_str()) {
                continue;
            }
            let path = routines_dir.join(format!("{name}.yaml"));
            let mut retired_detail = None;
            if path.exists() {
                let existing = fs::read_to_string(&path).map_err(|error| {
                    OrbitError::Io(format!(
                        "read retired managed routine '{}': {error}",
                        path.display()
                    ))
                })?;
                let byte_exact = sha256_hex(existing.as_bytes()) == *rendered_digest;
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

        if let Some(provenance) = previous_provenance {
            let binding = if overwrite_bindings {
                requested_binding.clone()
            } else {
                identity.complete(name, template, &provenance.binding)
            };
            let rendered = render_routine_template(name, template, &binding)?;
            let rendered_digest = sha256_hex(rendered.as_bytes());
            if path.exists() {
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
                    write_text_with_parent(&path, &rendered)?;
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
                    write_text_with_parent(&path, &rendered)?;
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
            if !path.exists() {
                let rendered = render_routine_template(name, template, &requested_binding)?;
                let rendered_digest = sha256_hex(rendered.as_bytes());
                if mode == ManagedAssetReconcileMode::Apply {
                    write_text_with_parent(&path, &rendered)?;
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
                write_text_with_parent(&path, &rendered)?;
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
        if path.exists() {
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
                write_text_with_parent(&path, &rendered)?;
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
        mode,
        &mut result,
    )?;

    let next = ManagedAssetManifest {
        schema_version: ROUTINE_MANAGED_ASSET_MANIFEST_SCHEMA_VERSION,
        asset_kind: "routine".to_string(),
        assets: next_assets,
        routine_provenance: next_provenance,
    };
    if mode == ManagedAssetReconcileMode::Apply && previous.as_ref() != Some(&next) {
        let encoded = encode_managed_asset_manifest(&next)?;
        atomic_write_text(&manifest_path, &encoded).map_err(|error| {
            OrbitError::Io(format!(
                "write managed routine asset manifest '{}': {error}",
                manifest_path.display()
            ))
        })?;
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
/// seeding never writes to, so it is skipped with every other subdirectory.
/// A directory that cannot be listed yields nothing: reconciliation of the
/// managed defaults has already reported what it could not read.
fn top_level_routine_files(routines_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(routines_dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
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

/// Whether `orbit workspace sync` would remove the routine definition at
/// `path` from `routines_dir`'s active catalog.
///
/// Every surface that tells an operator to run the sync asks this first: the
/// advice is only true for a file reconciliation can reach — one the manifest
/// tracks under a name this Orbit no longer ships, or an untracked copy of a
/// retired template. An operator's own definition is preserved by design, so
/// naming the sync for it would loop forever [DANI-10502].
pub(crate) fn sync_retires_routine(routines_dir: &Path, path: &Path) -> bool {
    if path.parent() != Some(routines_dir) {
        // A `local/` definition, or a file outside this catalog entirely:
        // seeding writes neither and retires neither.
        return false;
    }
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return false;
    };
    if DEFAULT_ROUTINE_FILES.iter().any(|(name, _)| *name == stem) {
        return false;
    }
    let manifest_path = routines_dir.join(MANAGED_ASSET_MANIFEST_FILE);
    if load_managed_asset_manifest(&manifest_path, "routine", ManagedAssetLayout::YamlStem)
        .ok()
        .flatten()
        .is_some_and(|manifest| manifest.assets.contains_key(stem))
    {
        return true;
    }
    fs::read_to_string(path)
        .ok()
        .is_some_and(|existing| shipped_shape_of(stem, &existing) == Some(ShippedShape::Retired))
}

/// Which template family an on-disk managed routine is a lifecycle-only
/// variant of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShippedShape {
    /// Matches the template this Orbit ships for the stem.
    Current,
    /// Matches an earlier shape of a template this Orbit still ships.
    Superseded,
    /// Matches the last shape of a default this Orbit no longer ships.
    Retired,
}

/// The fields a shipped template owns. `enabled` is the operator's opt-in
/// knob, `hosts:` is a retired key operators were told to drop, and comments
/// are not fields at all — none of them counts as a local modification.
fn template_owned_shape(definition: &RoutineDefinition) -> RoutineDefinition {
    RoutineDefinition {
        enabled: false,
        legacy_hosts: None,
        ..definition.clone()
    }
}

/// Which shipped template `existing` — the on-disk document for `file_stem`
/// — differs from only in operator-owned lifecycle settings, rendered against
/// the document's own name. `None` means the document does not parse or a
/// template-owned field changed: a genuine local edit.
pub(crate) fn shipped_shape_of(file_stem: &str, existing: &str) -> Option<ShippedShape> {
    let definition = parse_routine_yaml(existing).ok()?;
    let binding = binding_of(&definition);
    let shape = template_owned_shape(&definition);
    // A template that needs an owner the document does not declare (a cron
    // document judged against the state template) cannot be that shape.
    let matches = |templates: &[(&str, &str)]| {
        templates
            .iter()
            .filter(|(stem, _)| *stem == file_stem)
            .any(|(_, template)| {
                render_routine_template(file_stem, template, &binding)
                    .ok()
                    .and_then(|rendered| parse_routine_yaml(&rendered).ok())
                    .is_some_and(|rendered| template_owned_shape(&rendered) == shape)
            })
    };
    if matches(DEFAULT_ROUTINE_FILES) {
        Some(ShippedShape::Current)
    } else if matches(SUPERSEDED_ROUTINE_TEMPLATES) {
        Some(ShippedShape::Superseded)
    } else if matches(RETIRED_ROUTINE_FILES) {
        Some(ShippedShape::Retired)
    } else {
        None
    }
}

/// Whether an on-disk managed routine is Orbit's: byte-identical to the
/// digest the manifest recorded, or a lifecycle-only variant of a template
/// this or a prior release shipped for `file_stem`.
pub(crate) fn is_orbit_written_routine(
    file_stem: &str,
    recorded_digest: &str,
    existing: &str,
) -> bool {
    sha256_hex(existing.as_bytes()) == recorded_digest
        || shipped_shape_of(file_stem, existing).is_some()
}

/// Reconcile a tracked managed routine whose bytes no longer match its
/// recorded digest. A lifecycle-only variant of the current template is
/// adopted as-is; one of a superseded (or retired) template is refreshed onto
/// the current template with the operator's `enabled` kept. Returns the
/// provenance to record, or `None` for a genuine local edit the caller
/// preserves.
#[allow(clippy::too_many_arguments)]
fn reconcile_lifecycle_variant(
    identity: &RoutineSeedIdentity,
    file_stem: &str,
    template: &str,
    template_digest: &str,
    path: &Path,
    existing: &str,
    mode: ManagedAssetReconcileMode,
    result: &mut ManagedAssetReconciliation,
) -> Result<Option<RoutineAssetProvenance>, OrbitError> {
    let Some(shape) = shipped_shape_of(file_stem, existing) else {
        return Ok(None);
    };
    // `shipped_shape_of` parsed the document, so this cannot fail. A cron
    // document refreshed onto a state template takes this host as its owner.
    let definition = parse_routine_yaml(existing)?;
    let binding = identity.complete(file_stem, template, &binding_of(&definition));
    match shape {
        ShippedShape::Current => {
            result.actions.push(ManagedAssetAction {
                name: file_stem.to_string(),
                path: path.to_path_buf(),
                outcome: ManagedAssetOutcome::Migrated,
                detail: Some(
                    "adopted the operator's lifecycle settings (`enabled`, dropped `hosts:`) on an otherwise current managed routine"
                        .to_string(),
                ),
            });
            Ok(Some(RoutineAssetProvenance {
                template_digest: template_digest.to_string(),
                rendered_digest: sha256_hex(existing.as_bytes()),
                binding,
            }))
        }
        ShippedShape::Superseded | ShippedShape::Retired => {
            let rendered = render_refresh(file_stem, template, &binding, existing)?;
            if mode == ManagedAssetReconcileMode::Apply {
                write_text_with_parent(path, &rendered)?;
            }
            result.refreshed += 1;
            result.actions.push(ManagedAssetAction {
                name: file_stem.to_string(),
                path: path.to_path_buf(),
                outcome: ManagedAssetOutcome::Refreshed,
                detail: Some(
                    "shipped routine template changed; refreshed a prior release's routine and kept its `enabled` setting"
                        .to_string(),
                ),
            });
            Ok(Some(RoutineAssetProvenance {
                template_digest: template_digest.to_string(),
                rendered_digest: sha256_hex(rendered.as_bytes()),
                binding,
            }))
        }
    }
}

/// Render the current template for `binding`, keeping the `enabled` choice of
/// the on-disk document being refreshed. A document that does not parse has
/// no choice to keep and gets the template default.
fn render_refresh(
    file_stem: &str,
    template: &str,
    binding: &RoutineMaterializationBinding,
    existing: &str,
) -> Result<String, OrbitError> {
    let rendered = render_routine_template(file_stem, template, binding)?;
    let Ok(existing) = parse_routine_yaml(existing) else {
        return Ok(rendered);
    };
    let template_enabled = parse_routine_yaml(&rendered)?.enabled;
    if existing.enabled == template_enabled {
        return Ok(rendered);
    }
    let refreshed = rewrite_enabled_line(&rendered, existing.enabled)?;
    let definition = parse_routine_yaml(&refreshed).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "default routine `{file_stem}` failed validation after keeping enabled={}: {error}",
            existing.enabled
        ))
    })?;
    if definition.enabled != existing.enabled {
        return Err(OrbitError::InvalidInput(format!(
            "default routine `{file_stem}` did not keep the operator's enabled={} setting",
            existing.enabled
        )));
    }
    Ok(refreshed)
}

/// Recover the materialization binding of an on-disk routine that predates
/// routine provenance, so it can be adopted instead of reported as a
/// collision forever.
///
/// `None` means the file is not one Orbit can manage from this template:
/// either it does not parse as a routine, or its recorded binding could not be
/// re-rendered later. Refusing those keeps a future reconcile from
/// hard-failing the whole workspace sync on a binding Orbit adopted but cannot
/// use.
fn adoptable_binding(
    identity: &RoutineSeedIdentity,
    file_stem: &str,
    template: &str,
    existing: &str,
) -> Option<RoutineMaterializationBinding> {
    let definition = parse_routine_yaml(existing).ok()?;
    let binding = identity.complete(file_stem, template, &binding_of(&definition));
    render_routine_template(file_stem, template, &binding).ok()?;
    Some(binding)
}

/// The binding an on-disk document declares: its name, and the owner and
/// branch of its state trigger when it has one.
fn binding_of(definition: &RoutineDefinition) -> RoutineMaterializationBinding {
    let state = definition.trigger.state.as_ref();
    RoutineMaterializationBinding {
        name: definition.name.clone(),
        owner_machine: state.map(|trigger| trigger.owner_machine.clone()),
        branch: state.map(|trigger| trigger.branch.clone()),
    }
}

impl RoutineMaterializationBinding {
    /// The values this binding renders, for a drift report.
    fn describe(&self) -> String {
        let mut described = format!("name '{}'", self.name);
        if let Some(owner_machine) = &self.owner_machine {
            described.push_str(&format!(", owner '{owner_machine}'"));
        }
        if let Some(branch) = &self.branch {
            described.push_str(&format!(", branch '{branch}'"));
        }
        described
    }
}

/// Render `template` against `binding`, failing closed on any placeholder the
/// binding cannot resolve and on a document that does not reproduce the
/// binding it was rendered from.
fn render_routine_template(
    file_stem: &str,
    template: &str,
    binding: &RoutineMaterializationBinding,
) -> Result<String, OrbitError> {
    let mut rendered = template.replace(ROUTINE_NAME_PLACEHOLDER, &binding.name);
    if let Some(owner_machine) = &binding.owner_machine {
        rendered = rendered.replace(OWNER_MACHINE_PLACEHOLDER, owner_machine);
    }
    if let Some(branch) = &binding.branch {
        rendered = rendered.replace(BASE_BRANCH_PLACEHOLDER, branch);
    }
    if let Some(placeholder) = [OWNER_MACHINE_PLACEHOLDER, BASE_BRANCH_PLACEHOLDER]
        .into_iter()
        .find(|placeholder| rendered.contains(placeholder))
    {
        return Err(OrbitError::InvalidInput(format!(
            "default routine `{file_stem}` needs `{placeholder}` resolved, which its recorded materialization binding does not carry"
        )));
    }
    let definition = parse_routine_yaml(&rendered).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "default routine `{file_stem}` failed validation with recorded name '{}': {error}",
            binding.name
        ))
    })?;
    let reproduced = binding_of(&definition);
    let reproduces = reproduced.name == binding.name
        && binding
            .owner_machine
            .as_ref()
            .is_none_or(|owner| reproduced.owner_machine.as_ref() == Some(owner))
        && binding
            .branch
            .as_ref()
            .is_none_or(|branch| reproduced.branch.as_ref() == Some(branch));
    if !reproduces {
        return Err(OrbitError::InvalidInput(format!(
            "default routine `{file_stem}` did not reproduce its recorded materialization binding"
        )));
    }
    Ok(rendered)
}

fn sanitize_routine_name_part(raw: &str) -> String {
    let lowered = raw.trim().to_ascii_lowercase();
    let mut out = String::with_capacity(lowered.len());
    for ch in lowered.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    out.trim_matches(|ch| ch == '-' || ch == '_').to_string()
}
