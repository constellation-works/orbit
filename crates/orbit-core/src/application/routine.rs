//! Default routine seeding [ORB-10129].
//!
//! Routines are workspace-authored YAML under `.orbit/routines/` — unlike
//! activities and jobs there is no global routines directory, so defaults
//! are seeded per workspace on `orbit init`. One placeholder is resolved at
//! seed time:
//!
//! - `__ORBIT_ROUTINE_NAME__` — routine names must be unique across all
//!   routine sources on a host, so the seeded name carries the registered
//!   workspace name as a suffix (`task-triage-<workspace-name>`) to keep two
//!   seeded source workspaces from colliding fail-closed. The suffix comes
//!   from the workspace name the operator registered, never from the checkout
//!   directory: two checkouts whose directories share a basename would
//!   otherwise seed the same names on one host [ORB-12107].
//!
//! Nothing else is machine-dependent [ORB-12236]: two hosts initializing the
//! same workspace name write byte-identical definitions. Seeded routines are
//! disabled when written; they exist so a fresh workspace gets reviewable,
//! opt-in schedules without silently enabling unattended work.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use orbit_automation::routines::loader::declared_routine_names;
use orbit_common::OrbitError;
use orbit_common::protocol::yaml::parse_routine_yaml;

use super::{
    MANAGED_ASSET_MANIFEST_FILE, ManagedAssetAction, ManagedAssetLayout, ManagedAssetManifest,
    ManagedAssetOutcome, ManagedAssetReconcileMode, ManagedAssetReconciliation,
    ROUTINE_MANAGED_ASSET_MANIFEST_SCHEMA_VERSION, RoutineAssetProvenance,
    RoutineMaterializationBinding, encode_managed_asset_manifest, load_managed_asset_manifest,
    preserve_modified_retired_asset, sha256_hex,
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
        "task_triage",
        include_str!("../../assets/routines/task_triage.yaml"),
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

const ROUTINE_NAME_PLACEHOLDER: &str = "__ORBIT_ROUTINE_NAME__";

/// The identity a workspace's default routines are materialized against: the
/// registered workspace name their names are suffixed with.
///
/// Construction validates that name, which is why the field is private: an
/// existing value always renders a loadable routine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutineSeedIdentity {
    name_suffix: String,
}

impl RoutineSeedIdentity {
    /// Build the seed identity for `workspace_name`, rejecting a workspace
    /// name with no characters usable in a routine name — that name would
    /// otherwise silently fall back to a host-wide unsuffixed routine.
    pub fn new(workspace_name: &str) -> Result<Self, OrbitError> {
        let name_suffix = sanitize_routine_name_part(workspace_name);
        if name_suffix.is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "workspace name '{workspace_name}' has no characters usable in a routine name; \
                 routine names must stay unique across every routine source on this host, so \
                 choose a workspace name containing letters or digits"
            )));
        }

        Ok(Self { name_suffix })
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
    let identity = RoutineSeedIdentity::new(workspace_name)?;
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
            if path.exists() {
                let existing = fs::read_to_string(&path).map_err(|error| {
                    OrbitError::Io(format!(
                        "read retired managed routine '{}': {error}",
                        path.display()
                    ))
                })?;
                if sha256_hex(existing.as_bytes()) == *rendered_digest {
                    if mode == ManagedAssetReconcileMode::Apply {
                        fs::remove_file(&path).map_err(|error| {
                            OrbitError::Io(format!(
                                "retire managed routine '{}': {error}",
                                path.display()
                            ))
                        })?;
                    }
                } else {
                    let detail = format!(
                        "retired managed routine '{}' was locally modified; move or rename it before rerunning `orbit workspace sync`",
                        path.display()
                    );
                    if mode == ManagedAssetReconcileMode::Apply {
                        let preserved = preserve_modified_retired_asset(
                            routines_dir,
                            "routine",
                            ManagedAssetLayout::YamlStem,
                            name,
                            &path,
                        )?;
                        result.warnings.push(format!(
                            "{detail}; Orbit preserved it at '{}'",
                            preserved.display()
                        ));
                    } else {
                        result.warnings.push(detail.clone());
                    }
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
                detail: None,
            });
            result.retired += 1;
        }
    }

    for (name, template) in DEFAULT_ROUTINE_FILES {
        let path = routines_dir.join(format!("{name}.yaml"));
        let requested_binding = RoutineMaterializationBinding {
            name: identity.routine_name(name),
        };
        let template_digest = sha256_hex(template.as_bytes());
        let previous_digest = previous.as_ref().and_then(|value| value.assets.get(*name));
        let previous_provenance = previous
            .as_ref()
            .and_then(|value| value.routine_provenance.get(*name));

        if let Some(provenance) = previous_provenance {
            let binding = if overwrite_bindings {
                requested_binding.clone()
            } else {
                provenance.binding.clone()
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
                if !overwrite_bindings && provenance.binding != requested_binding {
                    result.actions.push(ManagedAssetAction {
                        name: (*name).to_string(),
                        path: path.clone(),
                        outcome: ManagedAssetOutcome::BindingDrift,
                        detail: Some(format!(
                            "current workspace binding would render name '{}'; preserving recorded name '{}'",
                            requested_binding.name, provenance.binding.name
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
            let binding = RoutineMaterializationBinding {
                name: definition.name,
            };
            let rendered = render_routine_template(name, template, &binding)?;
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
                if previous.is_none()
                    && let Some(binding) = adoptable_binding(name, template, &existing)
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
    file_stem: &str,
    template: &str,
    existing: &str,
) -> Option<RoutineMaterializationBinding> {
    let definition = parse_routine_yaml(existing).ok()?;
    let binding = RoutineMaterializationBinding {
        name: definition.name,
    };
    render_routine_template(file_stem, template, &binding).ok()?;
    Some(binding)
}

fn render_routine_template(
    file_stem: &str,
    template: &str,
    binding: &RoutineMaterializationBinding,
) -> Result<String, OrbitError> {
    let rendered = template.replace(ROUTINE_NAME_PLACEHOLDER, &binding.name);
    let definition = parse_routine_yaml(&rendered).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "default routine `{file_stem}` failed validation with recorded name '{}': {error}",
            binding.name
        ))
    })?;
    if definition.name != binding.name {
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

#[cfg(test)]
mod tests {
    use orbit_types::workflow::{OverlapPolicy, RoutineTarget};
    use tempfile::tempdir;

    use crate::application::routines::parse_cron;

    use super::*;

    #[test]
    fn seeded_routines_are_valid_disabled_and_workspace_unique() {
        let root = tempdir().expect("create tempdir");
        let routines_dir = root.path().join(".orbit/routines");
        let seeded =
            seed_default_routines(&routines_dir, "My Repo!", true).expect("seed default routines");
        assert_eq!(seeded.refreshed, DEFAULT_ROUTINE_FILES.len());

        for (stem, target) in [
            ("ci_failure_sweep", "ci_failure_sweep_pipeline"),
            ("dependabot_alert_sweep", "dependabot_alert_sweep_pipeline"),
            ("task_triage", "task_triage_pipeline"),
            ("task_pilot", "task_pilot_pipeline"),
            ("ship_sweep", "workspace_ship_pipeline"),
            ("worktree_gc", "worktree_gc_pipeline"),
        ] {
            let yaml = std::fs::read_to_string(routines_dir.join(format!("{stem}.yaml")))
                .expect("read seeded routine");
            let definition = parse_routine_yaml(&yaml).expect("seeded routine parses fail-closed");
            assert_eq!(
                definition.name,
                format!("{}-my-repo", stem.replace('_', "-"))
            );
            assert_eq!(definition.target, RoutineTarget::Job(target.to_string()));
            assert_eq!(definition.policy.overlap, OverlapPolicy::Forbid);
            assert!(!definition.enabled);
        }

        // Cadence: every 40 minutes — deliberately sparser than the
        // ~20-minute ship sweep, and parseable by the scheduler.
        let triage = std::fs::read_to_string(routines_dir.join("task_triage.yaml"))
            .expect("read triage routine");
        let triage = parse_routine_yaml(&triage).expect("triage routine parses");
        assert_eq!(triage.trigger.cron, "15 * * * *");
        parse_cron(&triage.trigger.cron).expect("seeded cron parses");

        // Task-pilot may run up to ten five-task partitions in two waves,
        // each agent bounded to 30 minutes. Its 90-minute timeout covers
        // that maximum automatic batch plus deterministic preparation/apply.
        let pilot = std::fs::read_to_string(routines_dir.join("task_pilot.yaml"))
            .expect("read task-pilot routine");
        let pilot = parse_routine_yaml(&pilot).expect("task-pilot routine parses");
        assert_eq!(pilot.trigger.cron, "*/40 * * * *");
        assert_eq!(
            pilot.trigger.missed_run,
            orbit_types::workflow::MissedRunPolicy::Skip
        );
        assert_eq!(pilot.policy.timeout_minutes, 90);
        assert_eq!(pilot.policy.overlap, OverlapPolicy::Forbid);
        parse_cron(&pilot.trigger.cron).expect("task-pilot cron parses");

        let ship = std::fs::read_to_string(routines_dir.join("ship_sweep.yaml"))
            .expect("read ship routine");
        let ship = parse_routine_yaml(&ship).expect("ship routine parses");
        assert_eq!(
            ship.trigger.missed_run,
            orbit_types::workflow::MissedRunPolicy::Skip
        );
        assert_eq!(ship.trigger.cron, "*/20 * * * *");
        parse_cron(&ship.trigger.cron).expect("ship cron parses");

        let gc = std::fs::read_to_string(routines_dir.join("worktree_gc.yaml"))
            .expect("read worktree GC routine");
        let gc = parse_routine_yaml(&gc).expect("worktree GC routine parses");
        assert!(!gc.enabled);
        assert_eq!(gc.policy.overlap, OverlapPolicy::Forbid);
        assert_eq!(gc.trigger.cron, "35 * * * *");

        // The CI-failure sweep is hourly and must not stack with any other
        // shipped default: two schedules on the same minute would have the
        // seeded routines contend for the same host on every fire.
        let sweep = std::fs::read_to_string(routines_dir.join("ci_failure_sweep.yaml"))
            .expect("read CI-failure sweep routine");
        let sweep = parse_routine_yaml(&sweep).expect("CI-failure sweep routine parses");
        assert!(!sweep.enabled);
        assert_eq!(sweep.trigger.cron, "5 * * * *");
        assert_ne!(sweep.trigger.cron, triage.trigger.cron);
        assert_eq!(
            sweep.trigger.missed_run,
            orbit_types::workflow::MissedRunPolicy::Skip
        );
        assert_eq!(sweep.policy.overlap, OverlapPolicy::Forbid);
        parse_cron(&sweep.trigger.cron).expect("CI-failure sweep cron parses");

        let dependabot = std::fs::read_to_string(routines_dir.join("dependabot_alert_sweep.yaml"))
            .expect("read Dependabot sweep routine");
        let dependabot = parse_routine_yaml(&dependabot).expect("Dependabot sweep routine parses");
        assert!(!dependabot.enabled);
        assert_eq!(dependabot.trigger.cron, "25 3 * * *");
        assert_eq!(dependabot.policy.overlap, OverlapPolicy::Forbid);
        for occupied in ["5 * * * *", "15 * * * *", "35 * * * *", "*/20 * * * *"] {
            assert_ne!(dependabot.trigger.cron, occupied);
        }
        parse_cron(&dependabot.trigger.cron).expect("Dependabot sweep cron parses");
    }

    #[test]
    fn seeding_preserves_existing_files_unless_overwrite() {
        let root = tempdir().expect("create tempdir");
        let routines_dir = root.path().join("routines");
        seed_default_routines(&routines_dir, "workspace", false).expect("first seed");
        let path = routines_dir.join("worktree_gc.yaml");
        std::fs::write(&path, "user edited").expect("simulate user edit");

        let seeded = seed_default_routines(&routines_dir, "workspace", false).expect("re-seed");
        assert_eq!(seeded.refreshed, 0);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "user edited",
            "plain re-init must not clobber user edits"
        );

        seed_default_routines(&routines_dir, "workspace", true).expect("refresh defaults");
        let refreshed = std::fs::read_to_string(&path).expect("read refreshed");
        let definition = parse_routine_yaml(&refreshed).expect("refreshed routine parses");
        assert_eq!(definition.name, "worktree-gc-workspace");
        assert_eq!(
            definition.target,
            RoutineTarget::Job("worktree_gc_pipeline".to_string())
        );
        assert!(!definition.enabled);
    }

    #[test]
    fn plain_reinit_adds_a_new_missing_default_without_rewriting_existing_files() {
        let root = tempdir().expect("create tempdir");
        let routines_dir = root.path().join("routines");
        seed_default_routines(&routines_dir, "workspace", false).expect("first seed");

        let existing = routines_dir.join("task_triage.yaml");
        let original = std::fs::read(&existing).expect("read existing routine bytes");
        let missing = routines_dir.join("task_pilot.yaml");
        std::fs::remove_file(&missing).expect("remove newly introduced routine");

        let seeded =
            seed_default_routines(&routines_dir, "workspace", false).expect("plain re-init");
        assert_eq!(seeded.refreshed, 1, "only the missing default is created");
        assert_eq!(
            std::fs::read(&existing).expect("read existing routine bytes"),
            original,
            "plain re-init must preserve existing routines byte-for-byte"
        );

        let pilot = parse_routine_yaml(
            &std::fs::read_to_string(&missing).expect("read newly seeded task-pilot routine"),
        )
        .expect("newly seeded task-pilot routine parses");
        assert!(!pilot.enabled);
    }

    /// The recorded digest covers the *rendered* document, so re-seeding
    /// unchanged embedded content for the same workspace must not rewrite a
    /// single file — even under `overwrite`. A steady-state bootstrap can then
    /// run against a read-only routines directory.
    #[test]
    fn reseeding_unchanged_rendered_content_is_a_no_op_not_a_rewrite() {
        let root = tempdir().expect("create tempdir");
        let routines_dir = root.path().join("routines");
        seed_default_routines(&routines_dir, "workspace", true).expect("first seed");

        let before: Vec<(std::path::PathBuf, std::time::SystemTime)> = DEFAULT_ROUTINE_FILES
            .iter()
            .map(|(stem, _)| {
                let path = routines_dir.join(format!("{stem}.yaml"));
                let modified = std::fs::metadata(&path)
                    .and_then(|metadata| metadata.modified())
                    .expect("read seeded routine mtime");
                (path, modified)
            })
            .collect();

        let reseeded = seed_default_routines(&routines_dir, "workspace", true)
            .expect("re-seed unchanged rendered content");
        assert_eq!(reseeded.refreshed, 0, "unchanged routines must not rewrite");
        assert_eq!(reseeded.retired, 0);
        assert!(reseeded.warnings.is_empty());

        for (path, modified) in before {
            let current = std::fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .expect("read routine mtime after re-seed");
            assert_eq!(
                current,
                modified,
                "re-seed rewrote `{}` despite identical rendered content",
                path.display()
            );
        }

        // Seeding is machine-independent [ORB-12236], so re-seeding the same
        // workspace name on another host stays a no-op.
        let elsewhere = seed_default_routines(&routines_dir, "workspace", true)
            .expect("re-seed as another host would");
        assert_eq!(elsewhere.refreshed, 0);
    }

    #[test]
    fn fresh_routine_seeding_matches_rendered_canonical_templates() {
        let root = tempdir().expect("create tempdir");
        let routines_dir = root.path().join("routines");
        seed_default_routines(&routines_dir, "workspace", false).expect("seed canonical routines");

        for (stem, template) in DEFAULT_ROUTINE_FILES {
            let rendered = template.replace(
                ROUTINE_NAME_PLACEHOLDER,
                &format!("{}-workspace", stem.replace('_', "-")),
            );
            let seeded = std::fs::read_to_string(routines_dir.join(format!("{stem}.yaml")))
                .expect("read seeded routine");
            assert_eq!(
                seeded, rendered,
                "freshly seeded {stem} must match its rendered template"
            );
        }
    }

    #[test]
    fn task_pilot_reseeding_preserves_workspace_overrides() {
        let root = tempdir().expect("create tempdir");
        let routines_dir = root.path().join("routines");
        seed_default_routines(&routines_dir, "workspace", false).expect("seed canonical routines");
        let path = routines_dir.join("task_pilot.yaml");
        let edited = std::fs::read_to_string(&path)
            .expect("read task-pilot routine")
            .replace("enabled: false", "enabled: true")
            .replace("*/40 * * * *", "*/15 * * * *");
        let definition = parse_routine_yaml(&edited).expect("customized routine parses");
        assert!(definition.enabled);
        assert_eq!(definition.trigger.cron, "*/15 * * * *");
        std::fs::write(&path, &edited).expect("write operator overrides");

        seed_default_routines(&routines_dir, "workspace", false)
            .expect("reseed without overwriting workspace choices");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read preserved routine"),
            edited
        );
    }

    /// A routines directory seeded before routines carried managed-asset
    /// provenance has no manifest at all. Customizing a seeded routine —
    /// `enabled: true` — is the documented lifecycle, so it must be adopted
    /// into provenance rather than accused of colliding with the bundled
    /// default it came from [ORB-11154].
    #[test]
    fn manifestless_customized_routines_are_adopted_rather_than_called_collisions() {
        let root = tempdir().expect("create tempdir");
        let routines_dir = root.path().join("routines");
        seed_default_routines(&routines_dir, "workspace", false)
            .expect("seed a pre-provenance workspace");
        let manifest_path = routines_dir.join(MANAGED_ASSET_MANIFEST_FILE);
        std::fs::remove_file(&manifest_path).expect("drop the manifest to predate provenance");

        let customized = routines_dir.join("task_triage.yaml");
        let edited = std::fs::read_to_string(&customized)
            .expect("read seeded routine")
            .replace("enabled: false", "enabled: true");
        assert!(
            edited.contains("enabled: true"),
            "fixture must opt the routine in"
        );
        std::fs::write(&customized, &edited).expect("simulate the documented customization");

        let adopted = seed_default_routines(&routines_dir, "workspace", false)
            .expect("reconcile the pre-provenance directory");
        assert!(
            adopted.warnings.is_empty(),
            "customizing a seeded routine must not warn: {:?}",
            adopted.warnings
        );
        assert!(
            !adopted
                .actions
                .iter()
                .any(|action| action.outcome == ManagedAssetOutcome::Preserved),
            "no routine may be reported as a user-authored collision: {:?}",
            adopted.actions
        );
        assert!(adopted.actions.iter().any(|action| {
            action.name == "task_triage" && action.outcome == ManagedAssetOutcome::Migrated
        }));
        assert_eq!(
            std::fs::read_to_string(&customized).expect("reread routine"),
            edited,
            "adoption must not rewrite the operator's routine"
        );

        let manifest =
            load_managed_asset_manifest(&manifest_path, "routine", ManagedAssetLayout::YamlStem)
                .expect("load adopted manifest")
                .expect("adoption records a manifest");
        let provenance = manifest
            .routine_provenance
            .get("task_triage")
            .expect("the customized routine gains provenance");
        assert_eq!(provenance.rendered_digest, sha256_hex(edited.as_bytes()));
        assert_eq!(provenance.binding.name, "task-triage-workspace");

        // Provenance now owns the file, so convergence is a no-op instead of
        // repeating the same complaint on every run.
        let second =
            seed_default_routines(&routines_dir, "workspace", false).expect("second reconcile");
        assert!(second.warnings.is_empty());
        assert_eq!(second.refreshed, 0);
        assert!(second.actions.iter().any(|action| {
            action.name == "task_triage" && action.outcome == ManagedAssetOutcome::Unchanged
        }));
    }

    /// The distinguishing condition: once Orbit tracks the directory, a shipped
    /// name that the manifest does not claim really is user-authored, and is
    /// still reported [ORB-11154].
    #[test]
    fn user_authored_collision_is_still_reported_when_the_manifest_tracks_the_directory() {
        let root = tempdir().expect("create tempdir");
        let routines_dir = root.path().join("routines");
        seed_default_routines(&routines_dir, "workspace", false).expect("seed default routines");
        let manifest_path = routines_dir.join(MANAGED_ASSET_MANIFEST_FILE);
        let mut manifest =
            load_managed_asset_manifest(&manifest_path, "routine", ManagedAssetLayout::YamlStem)
                .expect("load manifest")
                .expect("seeding records a manifest");
        manifest.assets.remove("task_triage");
        manifest.routine_provenance.remove("task_triage");
        std::fs::write(
            &manifest_path,
            encode_managed_asset_manifest(&manifest).expect("encode manifest"),
        )
        .expect("write a manifest that never claimed this name");

        let user_authored = routines_dir.join("task_triage.yaml");
        let content = std::fs::read_to_string(&user_authored)
            .expect("read routine")
            .replace("task-triage-workspace", "my-own-triage");
        std::fs::write(&user_authored, &content).expect("write a user-authored routine");

        let reconciled = seed_default_routines(&routines_dir, "workspace", false)
            .expect("reconcile a tracked directory");
        assert!(
            reconciled
                .warnings
                .iter()
                .any(|warning| warning.contains("collides with bundled default")),
            "a tracked directory must still report a user-authored collision: {:?}",
            reconciled.warnings
        );
        assert!(reconciled.actions.iter().any(|action| {
            action.name == "task_triage" && action.outcome == ManagedAssetOutcome::Preserved
        }));
        assert_eq!(
            std::fs::read_to_string(&user_authored).expect("reread routine"),
            content
        );
    }

    /// Adoption needs a binding Orbit can re-render later. A file that does not
    /// parse as a routine yields none, so it stays a reported collision even in
    /// a manifest-less directory [ORB-11154].
    #[test]
    fn manifestless_unparseable_collision_is_still_reported() {
        let root = tempdir().expect("create tempdir");
        let routines_dir = root.path().join("routines");
        std::fs::create_dir_all(&routines_dir).expect("create routines dir");
        let path = routines_dir.join("task_triage.yaml");
        std::fs::write(
            &path,
            "not: a routine
",
        )
        .expect("write an unmanageable file");

        let reconciled = seed_default_routines(&routines_dir, "workspace", false)
            .expect("reconcile a manifest-less directory");
        assert!(
            reconciled
                .warnings
                .iter()
                .any(|warning| warning.contains("collides with bundled default")),
            "{:?}",
            reconciled.warnings
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("reread file"),
            "not: a routine\n"
        );
    }

    /// Without a usable workspace suffix every workspace on the host would
    /// seed the same bare `task-pilot` name, so seeding refuses the name
    /// instead of writing definitions that drop each other at load time.
    #[test]
    fn seeding_requires_a_workspace_name_with_usable_characters() {
        let root = tempdir().expect("create tempdir");
        let err = seed_default_routines(&root.path().join("routines"), " ***", true)
            .expect_err("unusable workspace name must not seed unsuffixed routines");
        assert!(err.to_string().contains("routine name"), "{err}");
    }

    /// The seeded suffix is the registered workspace name, so two checkouts
    /// whose directories share a basename still seed distinct names, and a
    /// name mismatch never leaks the directory into the routine [ORB-12107].
    #[test]
    fn seeded_names_follow_the_workspace_name_not_the_checkout_directory() {
        let alpha =
            RoutineSeedIdentity::new("Alpha QA").expect("workspace name renders a routine suffix");
        let beta = RoutineSeedIdentity::new("beta").expect("second workspace identity");

        assert_eq!(alpha.routine_name("task_pilot"), "task-pilot-alpha-qa");
        assert_eq!(beta.routine_name("task_pilot"), "task-pilot-beta");
        assert!(
            alpha
                .seeded_routine_names()
                .iter()
                .all(|name| !beta.seeded_routine_names().contains(name)),
            "distinct workspace names must not share a seeded routine name"
        );
    }

    /// A name another workspace on the host already declares is reported
    /// before seeding: routine discovery drops every colliding definition, so
    /// writing the duplicate would disable both workspaces' routines.
    #[test]
    fn collisions_report_names_another_workspace_already_declares() {
        let root = tempdir().expect("create tempdir");
        let other_orbit = root.path().join("other/.orbit");
        seed_default_routines(&other_orbit.join("routines"), "server", false)
            .expect("seed the other workspace");

        let identity = RoutineSeedIdentity::new("server").expect("seed identity");
        let collisions =
            default_routine_name_collisions(&identity, std::slice::from_ref(&other_orbit));
        assert_eq!(
            collisions.len(),
            DEFAULT_ROUTINE_FILES.len(),
            "every seeded name collides: {collisions:?}"
        );
        assert!(collisions.iter().any(|collision| {
            collision.name == "task-pilot-server"
                && collision.declared_in == other_orbit.join("routines/task_pilot.yaml")
        }));

        let distinct = RoutineSeedIdentity::new("other-server").expect("seed identity");
        assert!(
            default_routine_name_collisions(&distinct, &[other_orbit]).is_empty(),
            "a distinct workspace name must not collide"
        );
    }

    /// Local definitions share the host-wide name space, so a `local/`
    /// routine is detected too.
    #[test]
    fn collisions_cover_local_routine_definitions() {
        let root = tempdir().expect("create tempdir");
        let other_orbit = root.path().join("other/.orbit");
        let local_dir = other_orbit.join("routines/local");
        seed_default_routines(&other_orbit.join("routines"), "alpha", false)
            .expect("seed the other workspace");
        let local = std::fs::read_to_string(other_orbit.join("routines/task_pilot.yaml"))
            .expect("read a seeded routine to adapt")
            .replace("task-pilot-alpha", "task-pilot-beta");
        write_text_with_parent(&local_dir.join("pilot.yaml"), &local).expect("write local routine");

        let identity = RoutineSeedIdentity::new("beta").expect("seed identity");
        let collisions = default_routine_name_collisions(&identity, &[other_orbit]);
        assert_eq!(
            collisions
                .iter()
                .map(|collision| collision.name.as_str())
                .collect::<Vec<_>>(),
            vec!["task-pilot-beta"]
        );
    }
}
